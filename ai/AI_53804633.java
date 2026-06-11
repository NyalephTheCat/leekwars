import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_53804633 extends AI {
public AI_53804633() throws LeekRunException {
super(1, 4);
}
public void staticInit() throws LeekRunException {
}
private Object f_s() throws LeekRunException {
ops(1);return 5l;
}
public Object runIA(Session session) throws LeekRunException {
ops(1); return (Object) add(f_s(), 10l);
}
protected String getAIString() { return "Main2_1a99d3cf6db51_34";}
protected String[] getErrorFiles() { return new String[] {"shared", "Main2_1a99d3cf6db51_34", };}

protected int[] getErrorFilesID() { return new int[] {170176550, 53804633, };}

}
